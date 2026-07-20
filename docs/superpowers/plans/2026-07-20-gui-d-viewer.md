# GUI-d viewer F3 — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** F3 (`pane.view`) abre un visor a pantalla completa en la GUI (texto+encoding / hexview binario / recargar-como / scroll), intentando primero un previewer de plugin; teclas del viewer configurables (contexto `Viewer` del keymap). Extrae el `Viewer` puro de la TUI a `norte-frontend`.

**Architecture:** extraer `norte-tui::viewer::Viewer` (core puro, SIN i18n — el `status()` i18n se queda render-side) a `norte-frontend::viewer`; TUI consume por re-export (`just ci` verde). La GUI: sesión `OpenViewer` (preview→read acotado), estado `viewer: Option<Viewer>`, segundo resolver (Screen::Viewer), `run_viewer_command`, render virtualizado (uniform_list, #87) + status ES. Spec: `docs/superpowers/specs/2026-07-20-gui-d-viewer-design.md`.

**Tech Stack:** norte-frontend (norte-encoding ya presente); norte-tui (re-export + status i18n); norte-gui (GPUI, session, keymap GUI-c).

**Convenciones (cada task):** TDD donde hay lógica; `just ci` verde tras cada task del WORKSPACE (T1 frontend, T2 tui); norte-gui (T3) en su dir + manual. Español; commits `feat(frontend):`/`refactor(tui):`/`feat(gui):`.

**Datos verificados (2026-07-20, `crates/norte-tui/src/viewer.rs` + `main.rs` + proto/backend):**
- `Viewer` PURO salvo: `norte_i18n::t` (SOLO en `status()`) y `crate::app::display_name` (en `with_plugin_preview` — YA es `norte_frontend::display_name`). La ref "ratatui" es un comentario. Campos pub `path/truncated/hex/scroll`; privados `bytes/forced/plugin_preview/text/encoding_name/eol/had_errors/lines`. Métodos: `new`, `with_plugin_preview(path, plugin_name, output)`, `preview_plugin()->Option<&str>`, `cycle_encoding`, `reset_encoding`, `toggle_hex`, `total_rows`, `scroll_down/up(n)`, `scroll_top/bottom`, `rows(height)->Vec<String>` (tabs+controles→`�`, display-safe), `status()->String` (i18n, SE QUEDA). Consts `PAGE=10`, `HEX_COLS=16`, `TAB_WIDTH=8`. `PluginPreviewView{plugin_name, lines}`. Free `render_line`/`hex_rows` (priv).
- `status()` compone: encoding_name o `t("viewer-binary")`; ` t("viewer-forced")` si forced; EOL (`LF/CRLF/CR` o `t("eol-mixed")/t("eol-none")`) si `!hex`; `t("viewer-lossy")` si had_errors; `t("viewer-truncated")` si truncated.
- `norte_frontend::display_name` ya extraído (GUI-a). `norte-encoding` ya dep de norte-frontend.
- `norte_core::backend::Backend`: `read(&VPath, Option<ByteRange>)->Result<Vec<u8>,Error>`; `plugin_preview(&VPath)->Result<PluginPreviewResult,Error>` (EN el enum — la TUI llama `backend.plugin_preview(&path)`). `ByteRange{off,len}`. `FS_READ_MAX_CHUNK=8 MiB`.
- `PluginPreviewResult{ #[serde(flatten)] preview: Option<PluginPreview> }`; `PluginPreview{plugin_id, plugin_name, output: String}`.
- `Entry.size: Option<u64>` (marca truncated).
- TUI `open_viewer` (main.rs:2227): `backend.plugin_preview(&path)` → `Ok(Some(p))`→`with_plugin_preview`, `Ok(None)`/`Err`→`Viewer::new(bytes, truncated)`. Comandos `pane.view`/`viewer.close` en `run_command` (main.rs:2159+). Viewer resolver: `Effective::build_for(..., Screen::Viewer)` (main.rs:371). Tests del Viewer: `crates/norte-tui/tests/viewer.rs`.
- GUI (post GUI-c): `keymap.rs` (`COMMANDS`, `build_effective`, `gpui_chord`, orthodox.toml SIN `[viewer]`); `main.rs` (`resolver`, `run_command`, `on_key` modal→quick→resolver, `follow_cursor`, render `uniform_list`, `ROW_H`); `session.rs` (`SessionCmd{List,Submit,Cancel}`/`SessionEvent`, backend persistente); `modal.rs`.

---

### Task 1: `norte-frontend::viewer` — Viewer core sin i18n (tests movidos)

**Files:** Create `crates/norte-frontend/src/viewer.rs`; Modify `crates/norte-frontend/src/lib.rs`.

Crea el módulo con el Viewer core. NO se toca norte-tui (duplicación transitoria; `just ci` verde).

- [ ] **Step 1: escribe los tests que fallan** — en `crates/norte-frontend/src/viewer.rs`, `mod tests`, copia los tests de `crates/norte-tui/tests/viewer.rs` que ejercen el CORE (decode/detección, `hex_rows`/hexview, `cycle_encoding`, scroll+clamp, binario→hex, EOL, `with_plugin_preview` enmascara). Ajusta imports (`norte_frontend::viewer::Viewer` → `super::Viewer`; `crate::app::display_name` → `crate::display_name`). DEJA FUERA los que llamen `viewer.status()` (i18n) — ese método no se mueve; anótalo. Añade tests de los getters nuevos:
```rust
    #[test]
    fn getters_exponen_el_estado_para_el_status_del_frontend() {
        let v = Viewer::new(VPath::parse("mem:///a.txt").unwrap(), b"hola\n".to_vec(), false);
        assert_eq!(v.encoding_name(), "UTF-8");
        assert!(!v.is_forced());
        assert!(!v.had_errors());
        assert!(!v.hex);
    }
```

- [ ] **Step 2: rojo** — `cd crates/norte-frontend && cargo nextest run viewer` → FAIL (módulo inexistente).

- [ ] **Step 3: mueve el Viewer core** — crea `crates/norte-frontend/src/viewer.rs` copiando VERBATIM de `norte-tui/src/viewer.rs`: `PluginPreviewView`, `Viewer` (struct + todos los métodos EXCEPTO `status()`), `render_line`, `hex_rows`, consts `PAGE`/`HEX_COLS`/`TAB_WIDTH`. Cambios:
  1. Imports: `use norte_encoding::{Decoded, Detection, Eol};` + `use norte_proto::VPath;`. BORRA `use norte_i18n::t;`.
  2. `with_plugin_preview`: `crate::app::display_name` → `crate::display_name` (2 usos).
  3. BORRA el método `status()` (i18n) del `impl Viewer`.
  4. AÑADE getters (para que los frontends compongan el status):
```rust
    /// Nombre del encoding decodificado (`"UTF-8"`…), o `""` si es binario.
    #[must_use]
    pub fn encoding_name(&self) -> &str {
        self.encoding_name
    }
    /// El fin de línea detectado (solo significativo en modo texto).
    #[must_use]
    pub fn eol(&self) -> norte_encoding::Eol {
        self.eol
    }
    /// Hubo pérdidas al decodificar (bytes inválidos → `�`).
    #[must_use]
    pub fn had_errors(&self) -> bool {
        self.had_errors
    }
    /// El encoding está FORZADO por «recargar como…» (no es la detección).
    #[must_use]
    pub fn is_forced(&self) -> bool {
        self.forced.is_some()
    }
```
  `lib.rs`: `pub mod viewer;` + rustdoc.

- [ ] **Step 4: verde** — `cd crates/norte-frontend && cargo nextest run viewer && cargo clippy -p norte-frontend --all-targets -- -D warnings && cargo fmt --all`. `cargo nextest run -p norte-tui` sigue verde (intacta).

- [ ] **Step 5: Commit** — `feat(frontend): Viewer core (texto/hex/encoding) en norte-frontend (GUI-d T1)`

---

### Task 2: la TUI consume el Viewer (re-export + status i18n render-side)

**Files:** Modify `crates/norte-tui/src/viewer.rs`, `crates/norte-tui/src/ui.rs` (call-site de `status()`), `crates/norte-tui/tests/viewer.rs`.

- [ ] **Step 1: reescribe `norte-tui/src/viewer.rs`** — BORRA lo movido (`Viewer`, `PluginPreviewView`, `render_line`, `hex_rows`, consts, sus tests movidos). Deja SOLO el re-export + la composición i18n del status como fn LIBRE:
```rust
//! Viewer de la TUI: re-exporta el Viewer core de norte-frontend + compone el
//! texto de estado localizado (Fluent) sobre sus getters.
pub use norte_frontend::viewer::{PAGE, PluginPreviewView, Viewer};

use norte_encoding::Eol;
use norte_i18n::t;

/// Línea de estado localizada del viewer (lo que antes era `Viewer::status`):
/// encoding/binario, forzado, EOL, pérdidas, truncado — el usuario SIEMPRE sabe
/// qué ve (spec §6). Vive en la TUI (i18n) sobre los getters del Viewer core.
#[must_use]
pub fn status(v: &Viewer) -> String {
    use std::fmt::Write;
    let mut out = if v.encoding_name().is_empty() {
        t("viewer-binary")
    } else {
        v.encoding_name().to_owned()
    };
    if v.is_forced() {
        out.push(' ');
        out.push_str(&t("viewer-forced"));
    }
    if !v.hex {
        let eol = match v.eol() {
            Eol::Lf => "LF".to_owned(),
            Eol::CrLf => "CRLF".to_owned(),
            Eol::Cr => "CR".to_owned(),
            Eol::Mixed => t("eol-mixed"),
            Eol::None => t("eol-none"),
        };
        let _ = write!(out, "  {eol}");
    }
    if v.had_errors() {
        let _ = write!(out, "  {}", t("viewer-lossy"));
    }
    if v.truncated {
        let _ = write!(out, "  {}", t("viewer-truncated"));
    }
    out
}
```
   Conserva/mueve aquí los tests que ejercían `status()` (ahora `status(&v)`), localizados.

- [ ] **Step 2: call-site** — en `ui.rs` (`draw_viewer`), `viewer.status()` → `crate::viewer::status(viewer)`. Verifica `grep -rn "\.status()" crates/norte-tui/src` por si hay otro.

- [ ] **Step 3: verde workspace** — `cargo nextest run -p norte-tui && cargo nextest run -p norte-frontend && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`.

- [ ] **Step 4: `just ci`** — EXIT=0. Revisa rustdoc links a items ahora en otro crate.

- [ ] **Step 5: Commit** — `refactor(tui): la TUI consume el Viewer core de norte-frontend (GUI-d T2)`

---

### Task 3: GUI — pantalla viewer + previewer + contexto Viewer del keymap

**Files:** Modify `crates/norte-gui/src/keymap.rs`, `crates/norte-gui/src/keymap_presets/orthodox.toml`, `crates/norte-gui/src/session.rs`, `crates/norte-gui/src/main.rs`.

Trabaja DENTRO de `crates/norte-gui/`. Edición 2021 (sin let-chains).

- [ ] **Step 1: COMMANDS de viewer + `pane.view` + preset `[viewer]`** — en `keymap.rs`, añade `pane.view` a `COMMANDS` (Browse) y una lista nueva:
```rust
/// Comandos del contexto Viewer (pantalla del visor F3).
pub const VIEWER_COMMANDS: &[&str] = &[
    "viewer.close", "viewer.up", "viewer.down",
    "viewer.page-up", "viewer.page-down", "viewer.top", "viewer.bottom",
    "viewer.encoding", "viewer.encoding-auto", "viewer.hex",
];
```
   En `orthodox.toml` añade la sección `[viewer]` y el bind de F3 en `[pane]`:
```toml
# En [pane], junto a las demás:
    { on = ["f3"], run = "pane.view" },

[viewer]
keymap = [
    { on = ["escape"], run = "viewer.close" },
    { on = ["f3"], run = "viewer.close" },
    { on = ["f10"], run = "viewer.close" },
    { on = ["up"], run = "viewer.up" },
    { on = ["down"], run = "viewer.down" },
    { on = ["pgup"], run = "viewer.page-up" },
    { on = ["pgdn"], run = "viewer.page-down" },
    { on = ["home"], run = "viewer.top" },
    { on = ["end"], run = "viewer.bottom" },
    { on = ["f8"], run = "viewer.encoding" },
    { on = ["ctrl+r"], run = "viewer.encoding-auto" },
    { on = ["f4"], run = "viewer.hex" },
]
```

- [ ] **Step 2: `build_effectives` (dos contextos)** — en `keymap.rs`, cambia `build_effective` para devolver AMBOS resolvers, o añade uno nuevo. El motor valida cada contexto contra su lista de comandos, pero `build_for` valida el keymap ENTERO (global+pane / global+viewer) contra UN `known_commands`. Por eso pásale la UNIÓN de comandos:
```rust
/// La unión de comandos de Browse + Viewer (para validar el keymap ENTERO —
/// build_for mezcla `global` con el contexto de la pantalla).
fn all_commands() -> Vec<&'static str> {
    COMMANDS.iter().chain(VIEWER_COMMANDS).copied().collect()
}

/// Construye los dos `Effective` (Browse y Viewer) desde el preset + capas.
///
/// # Errors
/// El primer `KeymapError` de una capa.
pub fn build_effectives() -> Result<(Effective, Effective), KeymapError> {
    let preset = orthodox();
    let mut layers: Vec<KeymapFile> = Vec::new();
    for (dir, is_project) in layer_dirs() {
        let path = dir.join("keymap.toml");
        if let Ok(src) = std::fs::read_to_string(&path) {
            let mut kf = parse_keymap(&src)?;
            if is_project { kf.mark_project(); }
            layers.push(kf);
        }
    }
    let cmds = all_commands();
    let browse = Effective::build_for(&preset, &layers, &cmds, Screen::Browse)?;
    let viewer = Effective::build_for(&preset, &layers, &cmds, Screen::Viewer)?;
    Ok((browse, viewer))
}

/// Fallback: los dos `Effective` SOLO del preset (no puede fallar — test).
#[must_use]
pub fn build_effectives_preset_only() -> (Effective, Effective) {
    let preset = orthodox();
    let cmds = all_commands();
    (
        Effective::build_for(&preset, &[], &cmds, Screen::Browse).expect("preset browse válido"),
        Effective::build_for(&preset, &[], &cmds, Screen::Viewer).expect("preset viewer válido"),
    )
}
```
   (`build_effective`/`build_effective_preset_only` de GUI-c pueden quedar como azúcar `build_effectives().map(|(b,_)| b)` o borrarse si `main.rs` pasa a los dos.) Test: `build_effectives` con el preset OK.

- [ ] **Step 3: session `OpenViewer`** — en `session.rs`:
```rust
// SessionCmd: nueva variante
    /// Abre el visor de `path`: intenta un previewer de plugin, si no lee bytes.
    OpenViewer { path: VPath },

// Contenido del viewer que cruza a la GUI.
pub enum ViewerContent {
    /// Salida de un previewer de plugin (texto + nombre).
    Plugin { plugin_name: String, output: String },
    /// Bytes crudos (posiblemente truncados al presupuesto).
    Raw { bytes: Vec<u8>, truncated: bool },
}

// SessionEvent: nuevas variantes
    /// El visor de `path` está listo con su contenido.
    ViewerOpened { path: VPath, content: ViewerContent },
    /// No se pudo abrir el visor de `path` (error ya renderizable).
    ViewerFailed { path: VPath, error: String },
```
   En el `match cmd` del hilo tokio, brazo:
```rust
                    SessionCmd::OpenViewer { path } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move { open_viewer(&backend, path, &tx).await });
                    }
```
   Y la fn (preview primero, read acotado si None):
```rust
/// Intenta un previewer de plugin; si ninguno aplica, lee bytes acotados. El
/// `truncated` se estima con el `size` del stat (más bytes de los leídos).
async fn open_viewer(
    backend: &Backend,
    path: VPath,
    tx: &mpsc::UnboundedSender<SessionEvent>,
) {
    match backend.plugin_preview(&path).await {
        Ok(res) if res.preview.is_some() => {
            let p = res.preview.expect("is_some");
            let _ = tx.send(SessionEvent::ViewerOpened {
                path,
                content: ViewerContent::Plugin { plugin_name: p.plugin_name, output: p.output },
            });
            return;
        }
        Ok(_) => {} // ningún previewer aplica → vista cruda.
        Err(_) => {} // preview falló → intenta la vista cruda igual.
    }
    let budget = norte_proto::methods::FS_READ_MAX_CHUNK;
    match backend
        .read(&path, Some(norte_proto::ByteRange { off: 0, len: budget }))
        .await
    {
        Ok(bytes) => {
            let truncated = bytes.len() as u64 >= budget; // leímos el tope: puede haber más.
            let _ = tx.send(SessionEvent::ViewerOpened {
                path,
                content: ViewerContent::Raw { bytes, truncated },
            });
        }
        Err(e) => {
            let _ = tx.send(SessionEvent::ViewerFailed { path, error: format!("{e}") });
        }
    }
}
```
   (`ByteRange` — confirma el nombre de los campos: `off`/`len` según `transfer.rs`; ajusta si difieren. `truncated = leímos exactamente el tope` es una heurística conservadora; si el Entry.size está disponible en la GUI, puede pasarse, pero el tope-alcanzado basta.)

- [ ] **Step 4: estado + apply_event + run_viewer_command en main.rs** — añade a `struct NorteGui`:
```rust
    /// El visor abierto (F3), o `None` = dual-pane.
    viewer: Option<norte_frontend::viewer::Viewer>,
    /// Resolver del contexto Viewer (teclas del visor).
    viewer_resolver: norte_frontend::keymap::Resolver,
    /// Handle de scroll de la lista virtualizada del visor (#87).
    viewer_scroll: gpui::UniformListScrollHandle,
```
   Init en AMBOS constructores: construye los dos resolvers con `build_effectives()` (fallback `build_effectives_preset_only()` + banner); `viewer: None`; `viewer_scroll: UniformListScrollHandle::new()`.
   `apply_event`: nuevos brazos:
```rust
            SessionEvent::ViewerOpened { path, content } => {
                use norte_frontend::viewer::Viewer;
                use session::ViewerContent;
                self.viewer = Some(match content {
                    ViewerContent::Plugin { plugin_name, output } => {
                        Viewer::with_plugin_preview(path, plugin_name, &output)
                    }
                    ViewerContent::Raw { bytes, truncated } => Viewer::new(path, bytes, truncated),
                });
            }
            SessionEvent::ViewerFailed { path: _, error } => {
                self.errors[self.focus] = Some(format!("visor: {error}"));
            }
```
   `run_command` (Browse) gana `"pane.view" => self.open_viewer(cx)`:
```rust
    /// Abre el visor sobre la entrada seleccionada si es un archivo.
    fn open_viewer(&mut self, _cx: &mut Context<Self>) {
        let f = self.focus;
        if let Some(e) = self.panes[f].selected() {
            if e.kind == norte_proto::EntryKind::File {
                let _ = self.cmds.send(SessionCmd::OpenViewer { path: e.path.clone() });
            }
        }
    }
```
   Nuevo `run_viewer_command`:
```rust
    /// Ejecuta un comando del contexto Viewer sobre `self.viewer`.
    fn run_viewer_command(&mut self, cmd: &str, _cx: &mut Context<Self>) {
        use norte_frontend::viewer::PAGE as VPAGE;
        let Some(v) = self.viewer.as_mut() else { return };
        match cmd {
            "viewer.close" => { self.viewer = None; return; }
            "viewer.up" => v.scroll_up(1),
            "viewer.down" => v.scroll_down(1),
            "viewer.page-up" => v.scroll_up(VPAGE),
            "viewer.page-down" => v.scroll_down(VPAGE),
            "viewer.top" => v.scroll_top(),
            "viewer.bottom" => v.scroll_bottom(),
            "viewer.encoding" => v.cycle_encoding(),
            "viewer.encoding-auto" => v.reset_encoding(),
            "viewer.hex" => v.toggle_hex(),
            _ => {}
        }
        // Sigue el scroll del visor con uniform_list.
        if let Some(v) = self.viewer.as_ref() {
            self.viewer_scroll.scroll_to_item(v.scroll, gpui::ScrollStrategy::Nearest);
        }
    }
```
   (OJO borrow: `viewer.close` pone `self.viewer=None` y hace `return` ANTES del bloque de scroll que reborrow-ea `self.viewer`. El `let Some(v)=self.viewer.as_mut()` se suelta al final del match; el `if let Some(v)=self.viewer.as_ref()` es un reborrow nuevo. Si el checker se queja, saca el `scroll_to_item` a una var `let s = self.viewer.as_ref().map(|v| v.scroll);` fuera y aplica.)

- [ ] **Step 5: on_key routing** — al principio de `on_key`, si el visor está abierto, enruta al contexto Viewer (el modal/quick no aplican en el visor):
```rust
        // Visor abierto: las teclas van al contexto Viewer (no hay modal/quick aquí).
        if self.viewer.is_some() {
            if ks.modifiers.platform { cx.notify(); return; }
            if let Some(chord) = keymap::gpui_chord(
                &ks.key, ks.modifiers.control, ks.modifiers.alt, ks.modifiers.shift, ks.key_char.as_deref(),
            ) {
                match self.viewer_resolver.push(chord) {
                    norte_frontend::keymap::Resolution::Run(cmd) => self.run_viewer_command(&cmd, cx),
                    norte_frontend::keymap::Resolution::Pending(_) => {}
                    norte_frontend::keymap::Resolution::Reset => {}
                }
            } else {
                self.viewer_resolver.reset();
            }
            cx.notify();
            return;
        }
```
   (Colócalo tras el early-return del modal si prefieres, pero ANTES del flujo de Browse. El visor y el modal son mutuamente excluyentes en la práctica; el visor primero es correcto.)

- [ ] **Step 6: render del visor** — en `render`, si `self.viewer` es `Some`, pinta el visor a pantalla COMPLETA (en vez del dual-pane): cabecera (`path_display` del `v.path` + « via <plugin>» si `v.preview_plugin()`), las `v.rows(height)` VIRTUALIZADAS con `uniform_list` (id `"viewer"`, item_count `v.total_rows()`, cada fila `.h(px(ROW_H))` con `SharedString::from(row)`; `.track_scroll(&self.viewer_scroll)`), y una barra de status compuesta EN LA GUI (ES) a partir de los getters:
```rust
    /// Barra de estado del visor, en español (i18n de la GUI = GUI-e). Compone
    /// desde los getters del Viewer core (encoding/binario, forzado, EOL, lossy,
    /// truncado) — el usuario SIEMPRE sabe qué ve (spec §6).
    fn viewer_status(v: &norte_frontend::viewer::Viewer) -> String {
        use norte_encoding::Eol;
        let mut out = if v.encoding_name().is_empty() { "binario".to_string() } else { v.encoding_name().to_owned() };
        if v.is_forced() { out.push_str(" (forzado)"); }
        if !v.hex {
            let eol = match v.eol() {
                Eol::Lf => "LF", Eol::CrLf => "CRLF", Eol::Cr => "CR",
                Eol::Mixed => "EOL mixto", Eol::None => "sin EOL",
            };
            out.push_str(&format!("  {eol}"));
        }
        if v.had_errors() { out.push_str("  con pérdidas"); }
        if v.truncated { out.push_str("  truncado"); }
        out
    }
```
   El `viewer_status` es una fn libre PURA (testeable). El render del visor va en un método `render_viewer(&self, v, cx) -> impl IntoElement` análogo a `render_pane`.
   Añade `norte-encoding` a `crates/norte-gui/Cargo.toml` si no está (para `Eol` en `viewer_status`).

- [ ] **Step 7: build + tests + clippy** — `cd crates/norte-gui && cargo build -p norte-gui && cargo nextest run --bin norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo fmt` (revierte reordenado incidental). Tests: `build_effectives` OK, `viewer_status` con un Viewer de texto/binario (sin hazards crudos — reusa el patrón hostil), `run_viewer_command` (scroll/hex/close → efecto sobre el Viewer).

- [ ] **Step 8: verificación manual** — daemon + GUI. F3 sobre un `.txt` → visor con encoding (status ES); flechas/PgUp/PgDn scroll; F8 «recargar como…» cicla encoding; F4 hex toggle; Esc/F3/F10 cierra. F3 sobre un binario (`/bin/ls`) → hexview automático. F3 sobre un dir → no-op. Si hay un plugin previewer aprobado, F3 sobre su tipo → « via <plugin>». Archivo grande → status «truncado».

- [ ] **Step 9: Commit** — `feat(gui): visor F3 (texto/hex/encoding + previewer, teclas configurables) (GUI-d T3)`

---

### Task 4: cierre — reviewers + gate + spec + push

- [ ] **Step 1: reviewers** (controller):
  - **rust-reviewer** sobre el rango (norte-frontend viewer + refactor TUI + GUI): extracción sin cambio de comportamiento (re-export, status i18n equivalente), reglas duras, el routing viewer↔browse, borrows del `run_viewer_command`, fail-safe del keymap.
  - **encoding-auditor** sobre `viewer.rs` (movido) + `viewer_status` GUI + el render del visor: `rows()`/`render_line`/`hex_rows` siguen display-safe (tabs expandidos, controles→`�`); `with_plugin_preview` enmascara la salida del plugin (texto de un TERCERO); el status GUI no pinta encoding/EOL/nombre crudos; el hexview no decodifica a ciegas. `plugin.preview` es texto ajeno → confirmar enmascarado.
  - Aplica hallazgos con TDD.
- [ ] **Step 2: gate** — `just ci` EXIT=0 (frontend viewer + TUI). norte-gui aparte: `cargo build/clippy/nextest -p norte-gui`.
- [ ] **Step 3: cerrar spec** — estado → IMPLEMENTADO + desviaciones (status GUI en ES hardcodeado hasta GUI-e; truncated por tope-alcanzado; dos resolvers; `build_effective` de GUI-c → `build_effectives`).
- [ ] **Step 4: deuda** — issue si queda: preview de imágenes, búsqueda en el visor, indicador de secuencia Pending, unificar `viewer_status` TUI/GUI cuando la GUI tenga i18n (GUI-e).
- [ ] **Step 5: Commit + push** — `test: cierre GUI-d — reviewers + gate + spec IMPLEMENTADO (GUI-d T4)` + push del rango.

---

## Self-review del plan (hecho)

- **Cobertura spec:** extraer Viewer core sin i18n + getters (T1); TUI consume + status i18n render-side (T2); GUI con session OpenViewer (preview→read acotado), estado viewer, dos resolvers (Browse+Viewer), VIEWER_COMMANDS + `[viewer]` preset + `pane.view`/F3, `run_viewer_command`, on_key routing, render virtualizado + status ES (T3); reviewers+gate (T4). Previewer primero + cruda fallback (session `open_viewer`); teclas configurables (build_effectives, dos contextos); binario→hex (Viewer core); errores→banner sin panic. Fuera de alcance (imágenes, edición, búsqueda, i18n GUI) no tocado.
- **Tipos consistentes:** `Viewer`/`PluginPreviewView`/`PAGE` en T1 (norte-frontend), re-exportados por la TUI (T2), consumidos por la GUI (T3). `ViewerContent`/`SessionCmd::OpenViewer`/`SessionEvent::{ViewerOpened,ViewerFailed}` en session (T3). `build_effectives`/`VIEWER_COMMANDS`/`run_viewer_command`/`viewer_status` en la GUI (T3). Getters (`encoding_name`/`eol`/`had_errors`/`is_forced`) definidos en T1, consumidos por `status` (TUI T2) y `viewer_status` (GUI T3).
- **Placeholders:** ninguno. Los puntos de borrow (`run_viewer_command` scroll, session `ByteRange` campos) van con la nota de cómo resolver.
- **Riesgo acotado:** T1 crea sin tocar la TUI (just ci verde, dup transitoria); T2 migra (tests del Viewer ya en frontend cazan regresiones; el status i18n se prueba localizado en la TUI); T3 es norte-gui excluido. El visor y el dual-pane son estados excluyentes (routing claro en on_key).
