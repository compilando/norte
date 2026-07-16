# M4-P5 — Plugins `previewer` en el viewer (F3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development o superpowers:executing-plans. Pasos con checkbox (`- [ ]`).

**Goal:** Al abrir un archivo con F3, si un plugin `previewer` APROBADO y ACTIVADO declara su mimetype, el core lo ejecuta sandboxeado sobre los bytes (que LEE el core, regla 9) y el viewer muestra su render en vez de la vista cruda — el caso estrella de plugins (issue #29).

**Architecture:** El core gana `plugin.preview {path}` (proto 0.15.0): detecta el mimetype del archivo por extensión (tabla pequeña, sin dep nueva), busca el PRIMER previewer aprobado+activado cuyos `mimetypes` (globs `text/*`) matcheen, LEE los bytes él mismo (el previewer no toca el FS — recibe bytes, regla 9), lo instancia con el runtime de M4-P2 y llama a su export `previewer.render`. Devuelve `{plugin_id, plugin_name, output}` o "ninguno" (el viewer cae a la vista normal). La ejecución respeta regla 2 (resolver bajo lock + ejecutar en spawn_blocking) y redacta errores de runtime (heredado de M4-P4). El TUI viewer (F3) intenta `backend.plugin_preview(path)`; si hay resultado, muestra el texto del plugin con un indicador "via <plugin>"; si no, la vista cruda de siempre.

**Tech Stack:** Rust, JSON-RPC (proto), wasmtime (runtime M4-P2), ratatui (viewer), `wasm32-wasip2` (guest E2E). Reviewers: **protocol-guardian OBLIGATORIO** (Task 1), **security-reviewer** (Task 2: el core lee bytes y los pasa —tope de tamaño anti-DoS—, fail-closed approved+enabled, redacción), rust-reviewer por task, encoding-auditor (Task 4: el output del plugin es texto de un TERCERO pintado en el viewer — enmascarar).

**Nota de threat model:** el previewer corre en el sandbox de M4-P2 (WASI vacío + límites CPU/memoria). El core lee el archivo (es el daemon, ya autorizado) y le pasa los bytes; el previewer nunca toca el FS. Solo se ejecutan previewers aprobados+activados (consentidos). El OUTPUT del plugin es texto no confiable → el viewer lo enmascara como cualquier contenido hostil.

---

## File Structure

- `crates/norte-proto/src/methods.rs` — `plugin.preview` + tipos + goldens (modificar).
- `crates/norte-core/src/plugins.rs` — detección de mimetype + `PluginRegistry::resolve_previewer`/`preview` (modificar).
- `crates/norte-core/src/daemon/server.rs` — handler (modificar).
- `crates/norte-core/src/backend.rs` — `Backend::plugin_preview` (modificar).
- `crates/norte-tui/src/viewer.rs` / `main.rs` / `ui.rs` — F3 intenta el previewer (modificar).
- `crates/norte-plugin-host/examples-wasm/previewer-demo/` — el guest de M4-P2 (reusar en el E2E).
- E2E: `crates/norte-core/tests/`.

---

## Task 1: proto 0.15.0 — `plugin.preview`

**Files:** `crates/norte-proto/src/methods.rs`, tests `types.rs`/`golden_types.rs`, goldens `methods.json`, test N-1 del daemon.

- [ ] **Step 1: const y tipos.** En `methods.rs`:

```rust
/// `plugin.preview` — ejecuta el primer plugin `previewer` APROBADO y
/// ACTIVADO que maneje el mimetype del archivo (M4-P5), sobre los bytes que el
/// core lee. `plugin_id`/`output` ausentes = ningún previewer aplica (el
/// frontend cae a la vista cruda).
pub const PLUGIN_PREVIEW: &str = "plugin.preview";

/// Params de [`PLUGIN_PREVIEW`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewParams {
    /// Archivo a previsualizar.
    pub path: VPath,
}

/// Result de [`PLUGIN_PREVIEW`]. Todo `None` = ningún previewer aplica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewResult {
    /// Id del plugin que renderizó (si alguno aplicó).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Nombre del plugin (para el indicador "via <plugin>").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
    /// Texto renderizado por el plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}
```

(Los tres `Option` con `skip_serializing_if` — patrón de `agent_session`; un result "ninguno" serializa a `{}`.)

- [ ] **Step 2: bump** `PROTOCOL_VERSION` → `"0.15.0"` + changelog. Ventana N/N-1 → 0.15/0.14, reject 0.13: `types.rs` `version_ventana_actual`, `golden_types.rs` assert, `daemon.rs` (los `0.13.x`/`0.13.0`/"de 0.14.0" → `0.14.x`/`0.14.0`/"de 0.15.0").

- [ ] **Step 3: tests** — roundtrip en `types.rs` (result poblado y result vacío `{}` = todo None); goldens `plugin_preview_params` + `plugin_preview_result` (poblado) + `plugin_preview_result_none` (`{}`) en `methods.json`; añadir al `check_methods_plugin` (sube el count); pin del const en `method_names_frozen`.

- [ ] **Step 4: verde** — `cargo nextest run -p norte-proto` y `-p norte-core -E 'binary(daemon)'`. **protocol-guardian OBLIGATORIO**. Commit: `feat(proto): plugin.preview, bump 0.15.0 + goldens (M4-P5 T1)`.

---

## Task 2: core — detección de mimetype + ejecutar el previewer

**Files:**
- Modify: `crates/norte-core/src/plugins.rs`
- Test: en `plugins.rs`

- [ ] **Step 1: detección de mimetype por extensión.** Función pura en `plugins.rs` (sin dep nueva): `fn guess_mimetype(path: &VPath) -> &'static str` que mira la extensión del último segmento y mapea una tabla pequeña:

```rust
/// Adivina el mimetype por EXTENSIÓN (heurística ligera, sin dep de sniffing).
/// Suficiente para casar los `mimetypes` que declara un previewer; un archivo
/// sin extensión conocida cae a `application/octet-stream` (ningún previewer
/// text/* lo tomará). NO lee el contenido.
fn guess_mimetype(path: &VPath) -> &'static str {
    let ext = path
        .file_name()
        .and_then(|n| std::str::from_utf8(n).ok())
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()));
    match ext.as_deref() {
        Some("txt" | "md" | "rs" | "toml" | "log" | "csv" | "ini" | "conf") => "text/plain",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        _ => "application/octet-stream",
    }
}
```

(Confirma la API de `VPath::file_name` — devuelve `Option<&[u8]>` de los bytes del último segmento; el codec existe. Si el nombre no es UTF-8, sin extensión reconocible → octet-stream.)

- [ ] **Step 2: match de mimetype contra los globs del previewer.** Los previewers declaran `mimetypes: Vec<String>` en `manifest.contributions.previewer[].mimetypes` (glob simple `text/*`). Función:

```rust
/// ¿El glob `pat` (p. ej. `text/*` o `application/json`) casa `mime`?
fn mimetype_matches(pat: &str, mime: &str) -> bool {
    match pat.strip_suffix("/*") {
        Some(prefix) => mime.split('/').next() == Some(prefix),
        None => pat == mime,
    }
}
```

- [ ] **Step 3: resolver + ejecutar.** En `plugins.rs`, siguiendo el patrón de `resolve_runnable`/`run_command` de M4-P4 (resolver bajo lock, ejecutar en spawn_blocking):

```rust
    /// Resuelve el PRIMER previewer aprobado+activado cuyo mimetype declarado
    /// case `mime`. Devuelve `(id, name, wasm_path, capabilities)` o `None`.
    /// Barato (sin I/O de ejecución): el caller lee los bytes y ejecuta fuera
    /// del lock (regla 2). Fail-closed: solo previewers consentidos.
    #[must_use]
    pub fn resolve_previewer(
        &self,
        mime: &str,
    ) -> Option<(String, String, std::path::PathBuf, norte_plugin_host::Capabilities)> {
        self.catalog.plugins.iter().find_map(|e| {
            let st = self.state.get(&e.manifest.id).copied().unwrap_or_default();
            if !st.approved || !st.enabled {
                return None;
            }
            let handles = e
                .manifest
                .contributions
                .previewer
                .iter()
                .flat_map(|c| c.mimetypes.iter())
                .any(|pat| mimetype_matches(pat, mime));
            if !handles {
                return None;
            }
            let wasm = e.dir.join("plugin.wasm");
            wasm.is_file().then(|| {
                (
                    e.manifest.id.clone(),
                    e.manifest.name.clone(),
                    wasm,
                    e.manifest.capabilities.clone(),
                )
            })
        })
    }
```

(Verifica que `manifest.contributions.previewer` y `PreviewerContrib.mimetypes` son accesibles —`pub`— en `norte-plugin-host`. Ya lo son según el modelo P1.)

La EJECUCIÓN (leer bytes + `runtime.instantiate(&wasm, caps)?.render_preview(mime, &bytes)?`) la hace el caller (daemon) con los bytes ya leídos y en spawn_blocking — igual que `run_command`. `PluginInstance::render_preview(mimetype, content) -> Result<String, RuntimeError>` ya existe (M4-P2). Añade en `plugins.rs` un helper si ayuda, pero la parte de leer el archivo la tiene el engine, no el registry — así que el ENSAMBLE (mime→resolve→leer→ejecutar) vive mejor en el handler del daemon o en un método del engine. DECISIÓN: `resolve_previewer` en el registry (arriba); el handler lee los bytes (via `shared.engine.read`) y ejecuta.

- [ ] **Step 4: tope de tamaño (anti-DoS).** El core lee el archivo entero en memoria para pasarlo al previewer. Define `const PREVIEW_MAX_BYTES: u64 = 1 * 1024 * 1024;` (1 MiB) y el caller lee como mucho eso (un preview de un archivo gigante no infla la memoria del host ni del guest — que además tiene su límite de M4-P2). Documenta el tope.

- [ ] **Step 5: tests** — en `plugins.rs`: `guess_mimetype` (varias extensiones + sin extensión); `mimetype_matches` (`text/*` casa `text/plain`, no `application/json`; exacto); `resolve_previewer` sobre un catálogo sembrado (un previewer `text/*` aprobado+activado → Some para `text/plain`, None para `application/json`; sin aprobar → None; sin `.wasm` → None). Commit: `feat(core): detección de mimetype + resolución de previewer (M4-P5 T2)`. **security-reviewer** (fail-closed, tope de tamaño) + rust-reviewer.

---

## Task 3: daemon + Backend — cablear `plugin.preview`

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (handler)
- Modify: `crates/norte-core/src/backend.rs` (`Backend::plugin_preview`)
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: handler** `plugin.preview` en dispatch (abierto, como `plugin.run_command`):

```rust
methods::PLUGIN_PREVIEW => {
    let p: methods::PluginPreviewParams = parse_params(req.params)?;
    handle_plugin_preview(p, shared).await
}
```

`handle_plugin_preview`:
1. `let mime = plugins::guess_mimetype(&p.path);` (hazla `pub(crate)` o expón lo necesario).
2. `let resolved = { shared.plugins.lock()...resolve_previewer(mime) };` (lock liberado).
3. Si `None` → `to_value(&PluginPreviewResult { plugin_id: None, plugin_name: None, output: None })` (el frontend cae a la vista cruda).
4. Si `Some((id, name, wasm, caps))`: lee los bytes con `shared.engine.read(&p.path, Some(range 0..PREVIEW_MAX_BYTES))` (drena el stream a un Vec, acotado); si el read falla, propaga como error de taxonomía. Luego `let rt = shared.plugin_runtime.clone(); let out = spawn_blocking(move || { rt.instantiate(&wasm, caps)?.render_preview(&mime_owned, &bytes) }).await`. Mapea `RuntimeError` REDACTADO (genérico "preview runtime failed" al cliente + warn local, como M4-P4). Devuelve `PluginPreviewResult { plugin_id: Some(id), plugin_name: Some(name), output: Some(out) }`.
   OJO: `mime` es `&'static str` → se mueve fácil al closure; `render_preview` toma `&str`. Ajusta ownership.

- [ ] **Step 2: Backend** `plugin_preview(&self, path: &VPath) -> Result<methods::PluginPreviewResult, Error>`. Embedded: registry efímero + `PluginRuntime::new()` + leer bytes via el engine, en spawn_blocking (o reusa la lógica; el embedded ya construye registry efímero para plugins_*). Remote: `call_timed` a `PLUGIN_PREVIEW`.

- [ ] **Step 3: test daemon** — `plugin.preview` de un path cuando NO hay previewer instalado → result todo `None` (no error). (El caso con previewer real = E2E.) Commit: `feat(core): daemon+Backend cablean plugin.preview (M4-P5 T3)`. rust-reviewer.

---

## Task 4: TUI — F3 intenta el previewer

**Files:**
- Modify: `crates/norte-tui/src/viewer.rs` (el estado del viewer gana un modo "preview de plugin")
- Modify: `crates/norte-tui/src/main.rs` (al abrir F3, intenta `plugin_preview`)
- Modify: `crates/norte-tui/src/ui.rs` (pinta el output del plugin, enmascarado, con indicador)
- Modify: `crates/norte-i18n/i18n/{es,en}.ftl`
- Test: `crates/norte-tui/tests/`

- [ ] **Step 1: modo preview en el viewer.** Mira `crates/norte-tui/src/viewer.rs` (`Viewer` struct: cómo guarda el contenido, texto/hex). Añade una variante/campo para "contenido renderizado por un plugin" (`plugin_preview: Option<(String /*plugin_name*/, Vec<String> /*líneas*/)>` o similar) — cuando está, el viewer pinta ESAS líneas en vez del hex/text crudo, con scroll. El texto del plugin se parte en líneas y se ENMASCARA por línea (`display_name` de app.rs — es texto de un TERCERO).

- [ ] **Step 2: al abrir F3.** En `main.rs`, el comando `pane.view` (mira cómo abre el viewer hoy: lee el archivo, construye `Viewer`). ANTES (o después) de construir el viewer normal, llama `backend.plugin_preview(&path).await`: si `output.is_some()`, construye el viewer en modo preview con el `plugin_name` y las líneas del output; si `None` o error, la vista cruda de siempre. (Un error del preview NO debe impedir ver el archivo: cae a crudo con un `app.message` opcional.)

- [ ] **Step 3: render + indicador.** En `ui.rs` `draw_viewer`: si el viewer está en modo preview, pinta las líneas del plugin y un indicador en el título/barra: `via <plugin_name>` (rol `Info`). Las líneas van por `display_name` (enmascarado). String i18n `viewer-plugin-preview = via { $plugin }` / `via { $plugin }`.

- [ ] **Step 4: test** — en `crates/norte-tui/tests/`: construye un `Viewer` en modo preview con líneas mock (una con un control char) y verifica que el render muestra el indicador `via <plugin>`, las líneas, y enmascara el control char. Commit: `feat(tui): F3 muestra el preview de un plugin si aplica (M4-P5 T4)`. rust-reviewer + encoding-auditor (output del plugin enmascarado).

---

## Task 5: E2E con el previewer real + cierre

**Files:**
- Test: `crates/norte-core/tests/` (usa el guest `previewer-demo` de M4-P2)
- Modify: `docs/adr/0022-*.md` (addendum P5), memoria

- [ ] **Step 1: E2E** — SKIP si falta el target wasm. Construye `previewer-demo` a `.wasm` (helper `build_guest` replicado, como en M4-P4 T5). Siembra `cfg/plugins/org.norte.prev/` con `plugin.toml` (category previewer, `[contributions] previewer = [{ mimetypes = ["text/*"] }]`, `[capabilities] fs-read = "scoped"`) + copia el `.wasm` a `plugin.wasm`. Además siembra un provider local o usa el engine con un archivo real (el previewer necesita bytes que el core lee — usa un `file://` de un tempfile con "linea uno\nlinea dos\n..."). Flujo por wire (daemon con plugins_dir=cfg + un provider que sirva el archivo): `plugin.preview {path}` sin aprobar el previewer → result `None` (cae a crudo); aprueba+activa; `plugin.preview` → `output` contiene el render del previewer-demo (que devuelve `"[text/plain] N bytes\n"` + 3 primeras líneas). Verifica `plugin_id`/`plugin_name` presentes.
   (Si el wire es engorroso por el provider del archivo, un E2E DIRECTO: `PluginRegistry::resolve_previewer` + leer el tempfile + `runtime.instantiate().render_preview()` — prueba el ensamble sin el daemon. Elige lo más limpio; el directo basta para el cierre si el wire de `read` complica.)

- [ ] **Step 2: verde total** — `just ci` COMPLETO.

- [ ] **Step 3: cierre** — addendum P5 en ADR 0022 (qué quedó: previewer plugins en F3 vía `plugin.preview`; detección de mimetype por extensión; el core lee bytes acotados y los pasa —regla 9—; output enmascarado; fail-closed; deuda: mimetype por sniffing de contenido (no solo extensión), varios previewers por mimetype con preferencia/orden, previewer en el PANE (no solo F3), streaming del preview para archivos grandes, las interfaces provider/columns/hook aún sin wiring). Memoria: **M4-P5 COMPLETA**. Cierra issue #29 si aplica. Commit: `docs,test: E2E previewer en el viewer + cierre M4-P5 (T5)`.

---

## Riesgos / verificar

1. **Regla 2**: `render_preview` (compila+instancia+ejecuta WASM) en spawn_blocking, nunca con el lock. Igual que `run_command` de M4-P4 — replica el patrón resolver/ejecutar.
2. **El core lee el archivo**: `shared.engine.read` con rango acotado (`PREVIEW_MAX_BYTES`). Un archivo gigante no infla memoria. Verifica que `engine.read` acepta un `ByteRange` con `len` y drena acotado.
3. **Output del plugin = texto no confiable**: el viewer lo enmascara por línea (`display_name`). encoding-auditor lo mira (Task 4).
4. **Fail-closed**: solo previewers aprobados+activados. Un previewer no consentido JAMÁS se ejecuta ni se consulta su render. Test que lo pinnee.
5. **`plugin.preview` no debe romper F3**: si el preview falla (error, timeout del guest), F3 cae a la vista cruda — jamás deja al usuario sin ver el archivo.
6. **Redacción**: errores de runtime del previewer NO filtran rutas al cliente (genérico + warn local), como M4-P4.
7. **`file_name`/extensión no-UTF8**: sin extensión reconocible → octet-stream → ningún previewer text/* aplica. No paniquea con bytes no-UTF8.
